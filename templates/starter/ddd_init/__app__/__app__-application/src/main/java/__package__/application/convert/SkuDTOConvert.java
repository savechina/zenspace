package __package__.application.convert;

import __package__.api.mo.SkuDTO;
import __package__.common.convert.DTOConverter;
import __package__.domain.model.sku.SkuDO;
import org.mapstruct.Mapper;

/**
 * SkuDTOConvert
 *
 * @author weirenyan
 * @date 2024/5/9
 */
@Mapper(componentModel = "spring")
public interface SkuDTOConvert  extends DTOConverter<SkuDTO, SkuDO> {
}
